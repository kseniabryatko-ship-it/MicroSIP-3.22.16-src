/*
 * Copyright (C) 2011-2026 MicroSIP (http://www.microsip.org)
 *
 * This program is free software; you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation; either version 2 of the License, or
 * (at your option) any later version.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU General Public License for more details.
 *
 * You should have received a copy of the GNU General Public License
 * along with this program; if not, write to the Free Software
 * Foundation, Inc., 59 Temple Place, Suite 330, Boston, MA  02111-1307  USA
 */

#include "stdafx.h"
#include "ContactsService.h"
#include "Markup.h"
#include "CSVFile.h"
#include "Utils/StringConverter.h"
#include "langpack.h"

#include <fstream>
#include <string>
#include <iostream>

ContactsService::ContactsService(std::unique_ptr<IContactsRepository> repository)
    : m_repository(std::move(repository))
{
}

bool ContactsService::Validate(const Contact& contact, CString* error)
{
    if (error) *error = "";
    return true;
}

bool ContactsService::ContactPrepare(Contact& contact)
{
    if (contact.number.IsEmpty()) {
        contact.number = contact.phone;
    }
    if (contact.number.IsEmpty()) {
        contact.number = contact.mobile;
    }
    if (contact.number.IsEmpty()) {
        return false;
    }
    if (contact.name.IsEmpty()) {
        if (contact.firstname != contact.lastname) {
            contact.name.Format(_T("%s %s"), contact.firstname, contact.lastname);
        }
        else {
            contact.name = contact.firstname;
        }
        contact.name.Trim();
    }
    if (contact.name.IsEmpty()) {
        contact.name = contact.number;
    }
    return true;

}



bool ContactsService::IsContactFiletered(const Contact& contact, CString search) {
    if (!search.IsEmpty()) {
        search.MakeLower();
        CString name = contact.name;
        CString number = contact.number;
        name.MakeLower();
        number.MakeLower();
        if (name.Find(search) == -1 && number.Find(search) == -1) {
            return true;
        }
    }
    return false;
}


void ContactsService::ExportContactsCSV(CString filename, CString search, CListCtrl* list)
{
    CCSVFile CSVFile;
    CSVFile.SetCodePage(CP_UTF8);
    if (CSVFile.Open((filename), CCSVFile::modeCreate | CCSVFile::modeWrite | CFile::typeText | CFile::shareExclusive)) {
        CStringArray arr;
        arr.Add(_T("Name"));
        arr.Add(_T("Number"));
        arr.Add(_T("First Name"));
        arr.Add(_T("Last Name"));
        arr.Add(_T("Phone Number"));
        arr.Add(_T("Mobile Number"));
        arr.Add(_T("E-mail Address"));
        arr.Add(_T("Address"));
        arr.Add(_T("City"));
        arr.Add(_T("State"));
        arr.Add(_T("Postal Code"));
        arr.Add(_T("Comment"));
        arr.Add(_T("Id"));
        arr.Add(_T("Info"));
        arr.Add(_T("Presence"));
        arr.Add(_T("Directory"));
        arr.Add(_T("Starred"));
        CSVFile.WriteData(arr);

        int count = list->GetItemCount();
        for (int i = 0; i < count; i++) {
            Contact* pContact = (Contact*)list->GetItemData(i);
            if (IsContactFiletered(*pContact, search)) {
                continue;
            }
            arr.RemoveAll();
            arr.Add((pContact->name));
            arr.Add((pContact->number));
            arr.Add((pContact->firstname));
            arr.Add((pContact->lastname));
            arr.Add((pContact->phone));
            arr.Add((pContact->mobile));
            arr.Add((pContact->email));
            arr.Add((pContact->address));
            arr.Add((pContact->city));
            arr.Add((pContact->state));
            arr.Add((pContact->zip));
            arr.Add((pContact->comment));
            arr.Add((pContact->id));
            arr.Add((pContact->info));
            arr.Add(pContact->presence ? _T("1") : _T("0"));
            arr.Add(pContact->directory ? _T("1") : _T("0"));
            arr.Add(pContact->starred ? _T("1") : _T("0"));
            CSVFile.WriteData(arr);
        }
        CSVFile.Close();
    }
}

void ContactsService::ExportContactsXML(CString filename, CString search, CListCtrl* list)
{
    CMarkup xml;
    xml.AddElem(_T("contacts"));
    xml.IntoElem();
    int count = list->GetItemCount();
    for (int i = 0; i < count; i++) {
        Contact* pContact = (Contact*)list->GetItemData(i);
        if (IsContactFiletered(*pContact, search)) {
            continue;
        }
        xml.AddElem(_T("contact"));
        xml.AddAttrib(_T("name"), (pContact->name));
        xml.AddAttrib(_T("number"), (pContact->number));
        xml.AddAttrib(_T("firstname"), (pContact->firstname));
        xml.AddAttrib(_T("lastname"), (pContact->lastname));
        xml.AddAttrib(_T("phone"), (pContact->phone));
        xml.AddAttrib(_T("mobile"), (pContact->mobile));
        xml.AddAttrib(_T("email"), (pContact->email));
        xml.AddAttrib(_T("address"), (pContact->address));
        xml.AddAttrib(_T("city"), (pContact->city));
        xml.AddAttrib(_T("state"), (pContact->state));
        xml.AddAttrib(_T("zip"), (pContact->zip));
        xml.AddAttrib(_T("comment"), (pContact->comment));
        xml.AddAttrib(_T("id"), (pContact->id));
        xml.AddAttrib(_T("info"), (pContact->info));
        xml.AddAttrib(_T("presence"), pContact->presence ? _T("1") : _T("0"));
        xml.AddAttrib(_T("starred"), pContact->starred ? _T("1") : _T("0"));
        xml.AddAttrib(_T("directory"), pContact->directory ? _T("1") : _T("0"));
    }
    std::ofstream file(filename, std::ios::binary);
    if (file) {
        std::string str = "<?xml version=\"1.0\"?>\r\n";
        str.append(CStringToUtf8(xml.GetDoc()));
        file.write(str.data(), str.size());
        file.close();
    }
}

CString ContactsService::ImportContactsCSV(CString filename, CArray<ContactWithFields*>& contactsWithFields, bool directory)
{
    CString result = _T("");
    CCSVFile CSVFile;
    CSVFile.SetCodePage(CP_UTF8);
    if (CSVFile.Open((filename), CCSVFile::modeRead | CFile::typeText | CFile::shareDenyWrite)) {
        CStringArray arr;
        int nameIndex = -1;
        int numberIndex = -1;
        int firstnameIndex = -1;
        int lastnameIndex = -1;
        int phoneIndex = -1;
        int mobileIndex = -1;
        int emailIndex = -1;
        int addressIndex = -1;
        int cityIndex = -1;
        int stateIndex = -1;
        int zipIndex = -1;
        int commentIndex = -1;
        int idIndex = -1;
        int infoIndex = -1;
        int presenceIndex = -1;
        int directoryIndex = -1;
        int starredIndex = -1;

        ContactWithFields* contactWithFields;

        bool bHeader = true;

        //m_repository->BeginTransaction();

        int nLine = 1;

        while (CSVFile.ReadData(arr)) {
            if (bHeader) {
                for (int i = 0; i < arr.GetCount(); i++) {
                    CString s = arr.GetAt(i);
                    if (nameIndex == -1 && arr.GetAt(i).CompareNoCase(_T("Name")) == 0) {
                        nameIndex = i;
                    }
                    if (numberIndex == -1 && (arr.GetAt(i).CompareNoCase(_T("Number")) == 0 || arr.GetAt(i).CompareNoCase(_T("Primary Phone")) == 0 || arr.GetAt(i).CompareNoCase(_T("phone")) == 0)) {
                        numberIndex = i;
                    }
                    if (firstnameIndex == -1 && (arr.GetAt(i).CompareNoCase(_T("First Name")) == 0 || arr.GetAt(i).CompareNoCase(_T("Given Name")) == 0 || arr.GetAt(i).CompareNoCase(_T("first_name")) == 0)) {
                        firstnameIndex = i;
                    }
                    if (lastnameIndex == -1 && (arr.GetAt(i).CompareNoCase(_T("Last Name")) == 0 || arr.GetAt(i).CompareNoCase(_T("Family Name")) == 0 || arr.GetAt(i).CompareNoCase(_T("last_name")) == 0)) {
                        lastnameIndex = i;
                    }
                    if (phoneIndex == -1 && (arr.GetAt(i).CompareNoCase(_T("Phone Number")) == 0 || arr.GetAt(i).CompareNoCase(_T("Home Phone")) == 0 || arr.GetAt(i).CompareNoCase(_T("Phone 2 - Value")) == 0 || arr.GetAt(i).CompareNoCase(_T("home_number")) == 0)) {
                        phoneIndex = i;
                    }
                    if (mobileIndex == -1 && (arr.GetAt(i).CompareNoCase(_T("Mobile Number")) == 0 || arr.GetAt(i).CompareNoCase(_T("Mobile Phone")) == 0 || arr.GetAt(i).CompareNoCase(_T("Phone 1 - Value")) == 0 || arr.GetAt(i).CompareNoCase(_T("mobile_number")) == 0)) {
                        mobileIndex = i;
                    }
                    if (emailIndex == -1 && (arr.GetAt(i).CompareNoCase(_T("E-mail Address")) == 0 || arr.GetAt(i).CompareNoCase(_T("E-mail 1 - Value")) == 0 || arr.GetAt(i).CompareNoCase(_T("email")) == 0)) {
                        emailIndex = i;
                    }
                    if (addressIndex == -1 && (arr.GetAt(i).CompareNoCase(_T("Address")) == 0 || arr.GetAt(i).CompareNoCase(_T("Home Address")) == 0)) {
                        addressIndex = i;
                    }
                    if (cityIndex == -1 && (arr.GetAt(i).CompareNoCase(_T("City")) == 0 || arr.GetAt(i).CompareNoCase(_T("Home City")) == 0)) {
                        cityIndex = i;
                    }
                    if (stateIndex == -1 && (arr.GetAt(i).CompareNoCase(_T("State")) == 0 || arr.GetAt(i).CompareNoCase(_T("Home State")) == 0)) {
                        stateIndex = i;
                    }
                    if (zipIndex == -1 && (arr.GetAt(i).CompareNoCase(_T("Postal Code")) == 0 || arr.GetAt(i).CompareNoCase(_T("Home Postal Code")) == 0)) {
                        zipIndex = i;
                    }
                    if (commentIndex == -1 && (arr.GetAt(i).CompareNoCase(_T("Comment")) == 0 || arr.GetAt(i).CompareNoCase(_T("Notes")) == 0)) {
                        commentIndex = i;
                    }
                    if (idIndex == -1 && arr.GetAt(i).CompareNoCase(_T("Id")) == 0) {
                        idIndex = i;
                    }
                    if (infoIndex == -1 && arr.GetAt(i).CompareNoCase(_T("Info")) == 0) {
                        infoIndex = i;
                    }
                    if (presenceIndex == -1 && arr.GetAt(i).CompareNoCase(_T("Presence")) == 0) {
                        presenceIndex = i;
                    }
                    if (directoryIndex == -1 && arr.GetAt(i).CompareNoCase(_T("Directory")) == 0) {
                        directoryIndex = i;
                    }
                    if (starredIndex == -1 && arr.GetAt(i).CompareNoCase(_T("Starred")) == 0) {
                        starredIndex = i;
                    }
                }
                if (numberIndex == -1 && phoneIndex == -1 && mobileIndex == -1) {
                    result = "The received data cannot be recognized";
                    break;
                }
                bHeader = false;
            }
            else {
                contactWithFields = new ContactWithFields();
                contactWithFields->contact.directory = directory;
                if (nameIndex != -1 && arr.GetCount() > nameIndex) {
                    contactWithFields->fields.AddTail(_T("name"));
                    contactWithFields->contact.name = (arr.GetAt(nameIndex));
                }
                if (numberIndex != -1 && arr.GetCount() > numberIndex) {
                    contactWithFields->fields.AddTail(_T("number"));
                    contactWithFields->contact.number = (arr.GetAt(numberIndex));
                }
                if (firstnameIndex != -1 && arr.GetCount() > firstnameIndex) {
                    contactWithFields->fields.AddTail(_T("firstname"));
                    contactWithFields->contact.firstname = (arr.GetAt(firstnameIndex));
                }
                if (lastnameIndex != -1 && arr.GetCount() > lastnameIndex) {
                    contactWithFields->fields.AddTail(_T("lastname"));
                    contactWithFields->contact.lastname = (arr.GetAt(lastnameIndex));
                }
                if (phoneIndex != -1 && arr.GetCount() > phoneIndex) {
                    contactWithFields->fields.AddTail(_T("phone"));
                    contactWithFields->contact.phone = (arr.GetAt(phoneIndex));
                }
                if (mobileIndex != -1 && arr.GetCount() > mobileIndex) {
                    contactWithFields->fields.AddTail(_T("mobile"));
                    contactWithFields->contact.mobile = (arr.GetAt(mobileIndex));
                }
                if (emailIndex != -1 && arr.GetCount() > emailIndex) {
                    contactWithFields->fields.AddTail(_T("email"));
                    contactWithFields->contact.email = (arr.GetAt(emailIndex));
                }
                if (addressIndex != -1 && arr.GetCount() > addressIndex) {
                    contactWithFields->fields.AddTail(_T("address"));
                    contactWithFields->contact.address = (arr.GetAt(addressIndex));
                }
                if (cityIndex != -1 && arr.GetCount() > cityIndex) {
                    contactWithFields->fields.AddTail(_T("city"));
                    contactWithFields->contact.city = (arr.GetAt(cityIndex));
                }
                if (stateIndex != -1 && arr.GetCount() > stateIndex) {
                    contactWithFields->fields.AddTail(_T("state"));
                    contactWithFields->contact.state = (arr.GetAt(stateIndex));
                }
                if (zipIndex != -1 && arr.GetCount() > zipIndex) {
                    contactWithFields->fields.AddTail(_T("zip"));
                    contactWithFields->contact.zip = (arr.GetAt(zipIndex));
                }
                if (commentIndex != -1 && arr.GetCount() > commentIndex) {
                    contactWithFields->fields.AddTail(_T("comment"));
                    contactWithFields->contact.comment = (arr.GetAt(commentIndex));
                }
                if (idIndex != -1 && arr.GetCount() > idIndex) {
                    contactWithFields->fields.AddTail(_T("id"));
                    contactWithFields->contact.id = (arr.GetAt(idIndex));
                }
                if (infoIndex != -1 && arr.GetCount() > infoIndex) {
                    contactWithFields->fields.AddTail(_T("info"));
                    contactWithFields->contact.info = (arr.GetAt(infoIndex));
                }
                if (presenceIndex != -1 && arr.GetCount() > presenceIndex) {
                    contactWithFields->fields.AddTail(_T("presence"));
                    contactWithFields->contact.presence = arr.GetAt(presenceIndex) == _T("1");
                }
                if (directoryIndex != -1 && arr.GetCount() > directoryIndex) {
                    contactWithFields->fields.AddTail(_T("directory"));
                    contactWithFields->contact.directory = arr.GetAt(directoryIndex) == _T("1");
                }
                if (starredIndex != -1 && arr.GetCount() > starredIndex) {
                    contactWithFields->fields.AddTail(_T("starred"));
                    contactWithFields->contact.starred = arr.GetAt(starredIndex) == _T("1");
                }
                if (ContactPrepare(contactWithFields->contact)) {
                    contactsWithFields.Add(contactWithFields);
                }
                else {
                    delete contactWithFields;
                }
            }
            nLine++;
        }
        //if (result.empty()) {
        //    m_repository->Commit();
        //}
        //else {
        //    m_repository->Rollback();
        //}
        CSVFile.Close();
    }
    else {
        result = "Cannot open file";
    }
    return result;
}

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

#pragma once

#include <memory>
#include <vector>
#include "../domain/Call.h"
#include "../data/ICallsRepository.h"

class CallsService
{
public:
    explicit CallsService(std::unique_ptr<ICallsRepository> repository);

    int AddCall(Call& log);
    void UpdateCall(const Call& log);
    void DeleteCall(int id);
    void DeleteCalls(std::vector<int> ids);
    void DeleteAllCalls();

    std::unique_ptr<Call> GetCall(int id);
    std::unique_ptr<Call> GetCallByCallId(const CString& callId);
    std::vector<std::unique_ptr<Call>> GetAllCalls();

    bool IsCallFiletered(const Call& call, CString search);

    void ExportCallsCSV(CString filename, CString search);

    std::string ImportCallsCSV(CString filename);

private:
    std::unique_ptr<ICallsRepository> m_repository;

    bool Validate(const Call& call, std::string* error = nullptr);
    
    void IniCallDecode(CString str, Call& call);
    std::vector<std::unique_ptr<Call>> IniCopyCalls();

};

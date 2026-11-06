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
#include "ICallsRepository.h"
#include <SQLiteCpp/SQLiteCpp.h>

class CallsRepository : public ICallsRepository
{
public:
    explicit CallsRepository(SQLite::Database& db);
    
    void BeginTransaction();
    void Commit();
    void Rollback();    

    int Add(const Call& call) override;
    void Update(const Call& call) override;
    void Delete(int id) override;
    void DeleteByIds(std::vector<int> ids);
    void DeleteAll() override;

    std::unique_ptr<Call> GetById(int id) override;
    std::unique_ptr<Call> GetByCallId(const CString& callId);
    std::vector<std::unique_ptr<Call>> GetAll(int limit = 0) override;

private:
    SQLite::Database& m_db;
    std::unique_ptr<Call> MapQueryToCall(SQLite::Statement& query);
    void BindCallFields(SQLite::Statement& query, const Call& call, bool includeId);
};
